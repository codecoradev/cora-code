//! MCP server — JSON-RPC 2.0 server over stdio transport.
//!
//! Reads JSON-RPC requests from stdin, dispatches to tool handlers,
//! writes responses to stdout.

use std::io::{self, Read, Write};

use tracing::{debug, error, info};

use super::protocol::{
    InitializeResult, JsonRpcError, JsonRpcRequest, JsonRpcResponse, RequestId, ServerCapabilities,
    ServerInfo,
};
use super::tools;

const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "cora-mcp";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Maximum size of a single JSON-RPC message (bytes). Larger messages are
/// discarded with a parse error instead of growing memory without bound.
const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

/// Result of feeding bytes to the [`Framer`].
#[derive(Debug, PartialEq, Eq)]
enum Frame {
    /// A complete, UTF-8 decoded top-level JSON object.
    Message(String),
    /// Framing failed; the framer has already reset and recovered.
    Error(String),
}

/// Incremental stdin framer.
///
/// Accumulates raw bytes (never `byte as char`) and decodes UTF-8 once a
/// top-level `{ ... }` object is balanced. Supports both newline-delimited and
/// pretty-printed multi-line objects. Multi-byte UTF-8 sequences consist solely
/// of bytes >= 0x80, so they can never be confused with the ASCII structural
/// characters tracked here.
struct Framer {
    buf: Vec<u8>,
    depth: usize,
    in_string: bool,
    escape: bool,
    /// Current message exceeded the size cap; keep tracking depth, drop bytes.
    overflow: bool,
    /// After a framing error outside an object, ignore input up to next newline.
    skip_line: bool,
    max_bytes: usize,
}

impl Framer {
    fn new(max_bytes: usize) -> Self {
        Self {
            buf: Vec::new(),
            depth: 0,
            in_string: false,
            escape: false,
            overflow: false,
            skip_line: false,
            max_bytes,
        }
    }

    fn reset(&mut self) {
        self.buf.clear();
        self.depth = 0;
        self.in_string = false;
        self.escape = false;
        self.overflow = false;
    }

    fn push(&mut self, bytes: &[u8], out: &mut Vec<Frame>) {
        for &b in bytes {
            self.push_byte(b, out);
        }
    }

    fn push_byte(&mut self, b: u8, out: &mut Vec<Frame>) {
        if self.skip_line {
            if b == b'\n' {
                self.skip_line = false;
            }
            return;
        }

        if self.depth == 0 {
            // Between messages.
            match b {
                b' ' | b'\t' | b'\r' | b'\n' => {}
                b'{' => {
                    self.depth = 1;
                    self.buf.push(b);
                }
                other => {
                    let shown = if other.is_ascii_graphic() {
                        format!("'{}'", other as char)
                    } else {
                        format!("0x{other:02x}")
                    };
                    out.push(Frame::Error(format!(
                        "unexpected {shown} outside of a JSON object"
                    )));
                    self.reset();
                    self.skip_line = true;
                }
            }
            return;
        }

        // Inside an object.
        if !self.overflow {
            if self.buf.len() >= self.max_bytes {
                out.push(Frame::Error(format!(
                    "message exceeds maximum size of {} bytes",
                    self.max_bytes
                )));
                self.buf.clear();
                self.buf.shrink_to_fit();
                self.overflow = true;
            } else {
                self.buf.push(b);
            }
        }

        if self.in_string {
            if self.escape {
                self.escape = false;
            } else if b == b'\\' {
                self.escape = true;
            } else if b == b'"' {
                self.in_string = false;
            }
            return;
        }

        match b {
            b'"' => self.in_string = true,
            b'{' => self.depth += 1,
            b'}' => {
                self.depth -= 1;
                if self.depth == 0 {
                    if !self.overflow {
                        let bytes = std::mem::take(&mut self.buf);
                        match String::from_utf8(bytes) {
                            Ok(s) => out.push(Frame::Message(s)),
                            Err(e) => out.push(Frame::Error(format!("invalid UTF-8: {e}"))),
                        }
                    }
                    self.reset();
                }
            }
            _ => {}
        }
    }

    /// Signal EOF. Reports a truncated message, if any.
    fn finish(&mut self, out: &mut Vec<Frame>) {
        if self.depth > 0 && !self.overflow {
            out.push(Frame::Error("unexpected end of input".to_string()));
        }
        self.reset();
    }
}

fn error_response(id: Option<RequestId>, code: i64, message: String) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message,
            data: None,
        }),
    }
}

/// Outcome of processing one framed message.
struct Processed {
    response: Option<JsonRpcResponse>,
    shutdown: bool,
}

/// Parse and dispatch a single JSON-RPC message.
fn process_message(text: &str) -> Processed {
    let value: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            error!(error = %e, "failed to parse request");
            return Processed {
                response: Some(error_response(None, -32700, format!("Parse error: {e}"))),
                shutdown: false,
            };
        }
    };

    let request: JsonRpcRequest = match serde_json::from_value(value.clone()) {
        Ok(req) => req,
        Err(e) => {
            // Valid JSON, but not a valid request (e.g. a stray response).
            // Only answer if it carried an id; otherwise it is notification-like.
            let id = value
                .get("id")
                .and_then(|v| serde_json::from_value::<RequestId>(v.clone()).ok());
            let response = id
                .is_some()
                .then(|| error_response(id, -32600, format!("Invalid Request: {e}")));
            return Processed {
                response,
                shutdown: false,
            };
        }
    };

    debug!(method = %request.method, "received request");

    // Notifications (no id) never get a response. Only an explicit
    // `shutdown` (or EOF) stops the server; `notifications/cancelled` only
    // cancels an in-flight request and must not terminate the session.
    if request.id.is_none() {
        handle_notification(&request);
        return Processed {
            response: None,
            shutdown: request.method == "shutdown",
        };
    }

    Processed {
        shutdown: request.method == "shutdown",
        response: Some(handle_request(&request)),
    }
}

fn handle_notification(request: &JsonRpcRequest) {
    match request.method.as_str() {
        "notifications/initialized" | "initialized" => debug!("client initialized"),
        "notifications/cancelled" => debug!("request cancelled by client"),
        m => debug!(method = m, "ignoring notification"),
    }
}

/// Drive the server loop over arbitrary reader/writer (stdio in production).
fn serve<R: Read, W: Write>(
    mut input: R,
    output: &mut W,
    max_message_bytes: usize,
) -> anyhow::Result<()> {
    let mut framer = Framer::new(max_message_bytes);
    let mut chunk = [0u8; 8192];
    let mut frames = Vec::new();

    loop {
        let n = match input.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        };
        framer.push(&chunk[..n], &mut frames);
        if dispatch_frames(&mut frames, output)? {
            return Ok(());
        }
    }

    framer.finish(&mut frames);
    dispatch_frames(&mut frames, output)?;
    Ok(())
}

/// Handle queued frames. Returns true when the server should shut down.
fn dispatch_frames<W: Write>(frames: &mut Vec<Frame>, output: &mut W) -> anyhow::Result<bool> {
    for frame in std::mem::take(frames) {
        match frame {
            Frame::Error(msg) => {
                error!(error = %msg, "framing error");
                write_response(
                    output,
                    &error_response(None, -32700, format!("Parse error: {msg}")),
                )?;
            }
            Frame::Message(text) => {
                let processed = process_message(&text);
                if let Some(resp) = processed.response {
                    write_response(output, &resp)?;
                }
                if processed.shutdown {
                    info!("Shutting down MCP server");
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

/// Run the MCP server, reading from stdin and writing to stdout.
pub fn run_server() -> anyhow::Result<()> {
    info!("Starting cora MCP server on stdio");
    let stdout = io::stdout();
    let mut stdout_lock = stdout.lock();
    serve(io::stdin().lock(), &mut stdout_lock, MAX_MESSAGE_BYTES)
}

fn handle_request(request: &JsonRpcRequest) -> JsonRpcResponse {
    match request.method.as_str() {
        "initialize" => handle_initialize(request),
        "tools/list" => handle_tools_list(request),
        "tools/call" => handle_tools_call(request),
        "ping" | "shutdown" => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: request.id.clone(),
            result: Some(serde_json::json!({})),
            error: None,
        },
        _ => error_response(
            request.id.clone(),
            -32601,
            format!("Method not found: {}", request.method),
        ),
    }
}

fn handle_initialize(request: &JsonRpcRequest) -> JsonRpcResponse {
    let result = InitializeResult {
        protocol_version: PROTOCOL_VERSION.to_string(),
        capabilities: ServerCapabilities {
            tools: serde_json::json!({}),
        },
        server_info: ServerInfo {
            name: SERVER_NAME.to_string(),
            version: SERVER_VERSION.to_string(),
        },
    };

    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id: request.id.clone(),
        result: Some(serde_json::to_value(result).unwrap_or_default()),
        error: None,
    }
}

fn handle_tools_list(request: &JsonRpcRequest) -> JsonRpcResponse {
    let tools = tools::list_tools();
    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id: request.id.clone(),
        result: Some(serde_json::json!({ "tools": tools })),
        error: None,
    }
}

fn handle_tools_call(request: &JsonRpcRequest) -> JsonRpcResponse {
    let tool_name = request
        .params
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if tool_name.trim().is_empty() {
        return error_response(
            request.id.clone(),
            -32602,
            "Invalid params: missing or empty tool 'name'".to_string(),
        );
    }

    let args = request
        .params
        .get("arguments")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    let result = tools::handle_tool_call(tool_name, &args);

    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id: request.id.clone(),
        result: Some(serde_json::to_value(result).unwrap_or_default()),
        error: None,
    }
}

fn write_response<W: Write>(out: &mut W, response: &JsonRpcResponse) -> anyhow::Result<()> {
    let json = serde_json::to_string(response)?;
    debug!(output = %json, "sending response");
    writeln!(out, "{json}")?;
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(input: &[u8]) -> Vec<serde_json::Value> {
        run_with_cap(input, MAX_MESSAGE_BYTES)
    }

    fn run_with_cap(input: &[u8], cap: usize) -> Vec<serde_json::Value> {
        let mut out = Vec::new();
        serve(input, &mut out, cap).unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn handle_initialize_response() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(RequestId::Number(1)),
            method: "initialize".to_string(),
            params: serde_json::json!({}),
        };
        let resp = handle_request(&req);
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(result["serverInfo"]["name"], SERVER_NAME);
    }

    #[test]
    fn handle_tools_list_response() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(RequestId::Number(2)),
            method: "tools/list".to_string(),
            params: serde_json::json!({}),
        };
        let resp = handle_request(&req);
        assert!(resp.result.is_some());
        let result = resp.result.unwrap();
        let tools = result["tools"].as_array().unwrap();
        assert!(!tools.is_empty());
    }

    #[test]
    fn handle_tools_call_list_rules() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(RequestId::Number(3)),
            method: "tools/call".to_string(),
            params: serde_json::json!({
                "name": "cora.list_rules",
                "arguments": {}
            }),
        };
        let resp = handle_request(&req);
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    #[test]
    fn handle_unknown_method() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(RequestId::Number(99)),
            method: "unknown/method".to_string(),
            params: serde_json::json!({}),
        };
        let resp = handle_request(&req);
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32601);
    }

    #[test]
    fn handle_ping() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(RequestId::Number(4)),
            method: "ping".to_string(),
            params: serde_json::json!({}),
        };
        let resp = handle_request(&req);
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    #[test]
    fn utf8_multibyte_roundtrips_intact() {
        // Japanese + emoji inside a string, delivered one byte at a time so
        // multi-byte sequences are split across reads.
        let msg = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"cora.check_snippet\",\"arguments\":{\"code\":\"let s = \\\"日本語🦀\\\";\"}}}\n";
        let mut framer = Framer::new(MAX_MESSAGE_BYTES);
        let mut frames = Vec::new();
        for b in msg.as_bytes() {
            framer.push(std::slice::from_ref(b), &mut frames);
        }
        assert_eq!(frames.len(), 1);
        match &frames[0] {
            Frame::Message(s) => {
                assert!(s.contains("日本語🦀"));
                assert!(serde_json::from_str::<serde_json::Value>(s).is_ok());
            }
            other => panic!("unexpected frame: {other:?}"),
        }

        let out = run(msg.as_bytes());
        assert_eq!(out.len(), 1);
        assert!(out[0]["error"].is_null());
    }

    #[test]
    fn braces_inside_strings_do_not_break_framing() {
        let msg =
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\",\"params\":{\"x\":\"}{ \\\" }\"}}\n";
        let out = run(msg.as_bytes());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["id"], 1);
    }

    #[test]
    fn pretty_printed_multiline_json() {
        let msg = "{\n  \"jsonrpc\": \"2.0\",\n  \"id\": 7,\n  \"method\": \"ping\",\n  \"params\": {\n    \"a\": {\n      \"b\": 1\n    }\n  }\n}\n";
        let out = run(msg.as_bytes());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["id"], 7);
        assert!(out[0]["result"].is_object());
    }

    #[test]
    fn two_messages_back_to_back() {
        let msg = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n";
        let out = run(msg.as_bytes());
        assert_eq!(out.len(), 2);
        assert_eq!(out[1]["id"], 2);
    }

    #[test]
    fn stray_closing_brace_recovers() {
        let input = "}\n{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"ping\"}\n";
        let out = run(input.as_bytes());
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["error"]["code"], -32700);
        assert_eq!(out[1]["id"], 5);
        assert!(out[1]["result"].is_object());
    }

    #[test]
    fn garbage_line_recovers_with_single_error() {
        let input = "not json at all }}}\n{\"jsonrpc\":\"2.0\",\"id\":6,\"method\":\"ping\"}\n";
        let out = run(input.as_bytes());
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["error"]["code"], -32700);
        assert_eq!(out[1]["id"], 6);
    }

    #[test]
    fn invalid_json_object_returns_parse_error_then_recovers() {
        let input = "{\"jsonrpc\": oops}\n{\"jsonrpc\":\"2.0\",\"id\":8,\"method\":\"ping\"}\n";
        let out = run(input.as_bytes());
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["error"]["code"], -32700);
        assert_eq!(out[1]["id"], 8);
    }

    #[test]
    fn invalid_utf8_returns_parse_error() {
        let mut input = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"p\xff\"}\n".to_vec();
        input.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n");
        let out = run(&input);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["error"]["code"], -32700);
        assert_eq!(out[1]["id"], 2);
    }

    #[test]
    fn truncated_message_at_eof_reports_parse_error() {
        let out = run(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"meth");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["error"]["code"], -32700);
    }

    #[test]
    fn oversized_message_rejected_and_stream_recovers() {
        let big = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\",\"params\":{{\"pad\":\"{}\"}}}}\n",
            "x".repeat(500)
        );
        let input = format!("{big}{{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}}\n");
        let out = run_with_cap(input.as_bytes(), 128);
        assert_eq!(out.len(), 2, "{out:?}");
        assert_eq!(out[0]["error"]["code"], -32700);
        assert!(
            out[0]["error"]["message"]
                .as_str()
                .unwrap()
                .contains("maximum size")
        );
        assert_eq!(out[1]["id"], 2);
    }

    #[test]
    fn notification_without_id_gets_no_response() {
        let input = "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n\
                     {\"jsonrpc\":\"2.0\",\"method\":\"notifications/whatever\",\"params\":{}}\n\
                     {\"jsonrpc\":\"2.0\",\"method\":\"unknown/notification\"}\n";
        let out = run(input.as_bytes());
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn cancelled_notification_does_not_shut_down() {
        let input = "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/cancelled\",\"params\":{\"requestId\":1}}\n\
                     {\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"ping\"}\n";
        let out = run(input.as_bytes());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["id"], 9);
    }

    #[test]
    fn shutdown_request_responds_then_stops() {
        let input = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"shutdown\"}\n\
                     {\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n";
        let out = run(input.as_bytes());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["id"], 1);
    }

    #[test]
    fn tools_call_missing_name_is_invalid_params() {
        for params in [
            serde_json::json!({}),
            serde_json::json!({"name": ""}),
            serde_json::json!({"name": 5}),
        ] {
            let req = JsonRpcRequest {
                jsonrpc: "2.0".to_string(),
                id: Some(RequestId::Number(1)),
                method: "tools/call".to_string(),
                params,
            };
            let resp = handle_request(&req);
            assert_eq!(resp.error.unwrap().code, -32602);
        }
    }
}
