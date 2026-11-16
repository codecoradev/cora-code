{{CORA_SKILL_MARKER}}

# Ignore before you index, review or scan

Review, scan and the code index read what is in the project. Dependencies, build output and generated code are the main source of noise (findings nobody should fix), of cost (tokens spent on code nobody wrote) and of poison: third-party and generated text goes into the prompt as untrusted data, and vendored symbols make "related code" point into `node_modules`. Set up ignores first, then review.

## What cora already ignores

At any depth, in review (the diff), scan and the index: `node_modules`, `vendor`, `bower_components`, `__pycache__`, `site-packages`, `.venv`, `venv`, `Pods`, `dist`, `build`, `out`, `target`, `.next`, `.nuxt`, `coverage`, `.gradle`, `.git`, `*.min.js`, `*.min.css`, `*.map` and the common lockfiles. Files in `.gitignore` are skipped by scan and index as well. See the effective list with `cora config show`.

## What to add

Look at the project for what the defaults do not cover, and ask the user before hiding anything that is not clearly generated or vendored:

- generated code (`*.pb.go`, `*_generated.*`, `src/generated/**`, API clients, schema output);
- vendored or copied third-party sources under a name the defaults do not know;
- build or report output in other places (`site/`, `docs/_build/`, `reports/`), snapshots, recorded fixtures, large data files;
- a directory that holds your own code under a default name (a maintained `vendor/`, a `build/` of scripts): set `ignore.defaults: false` and list what you want ignored yourself.

## How (in `.cora.yaml`)

```yaml
ignore:
  files:                 # out of scope for scan, index and review context (adds to the defaults)
    - "src/generated/**"
    - "**/*.pb.go"
  rules:                 # silences a rule; the files are still reviewed
    - rule: sec-hardcoded-url
      files: ["docs/**"]
      reason: documentation quotes example URLs
review:
  exclude: ["snapshots/**"]   # removed from the reviewed diff entirely
```

- `ignore.files` for files, `review.exclude` for files that must never reach the model, `ignore.rules` for a rule that is wrong in one place (always with a `reason`, and scoped with `files` when it can be), a `cora-ignore:` comment for a single finding.
- Patterns: `**/` matches at any depth, `dir/**` everything under a directory, `*.ext` a suffix.

## What not to ignore

- Your own source and your tests, to make a report shorter.
- A rule everywhere because it fired once: it hides the same mistake in your own code too.
- Anything just to get a review to pass. A finding you disagree with gets an `ignore.rules` entry with a reason, so the next reader sees why.

## Check it

After a change, `cora index` rebuilds the index with the new list. `cora scan --no-llm` shows what the deterministic scan still reports, and a review says how many files of the diff the built-in ignores skipped.
