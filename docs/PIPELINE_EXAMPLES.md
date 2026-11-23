# Pipeline examples

## Rust CLI: test, matrix build, release

`.ubuilder/workflows/desktop-release.yml` in this repository:

```yaml
version: 1
name: Desktop Release
"on":
  manual: true
env:
  CARGO_TERM_COLOR: always
jobs:
  test:
    name: Unit tests
    steps:
      - uses: builtin/checkout@v1
      - uses: builtin/setup-rust@v1
        with:
          channel: stable
      - run: cargo test --locked
  build:
    name: Build ${{ matrix.target }}
    needs: [test]
    strategy:
      matrix:
        target: [windows-x64]
    steps:
      - uses: builtin/checkout@v1
      - uses: builtin/setup-rust@v1
      - uses: builtin/build@v1
        with:
          target: ${{ matrix.target }}
          profile: release
      - uses: builtin/package@v1
        with:
          format: zip
      - uses: builtin/upload-artifact@v1
        with:
          name: desktop-${{ matrix.target }}
          path: dist
  release:
    needs: [build]
    steps:
      - uses: builtin/download-artifact@v1
        with:
          pattern: desktop-*
          path: dist
      - uses: builtin/checksum@v1
        with:
          path: dist
          output: dist/checksums.sha256
      - uses: builtin/upload-artifact@v1
        with:
          name: release
          path: dist/checksums.sha256
```

```console
ubuilder pipeline validate
ubuilder pipeline plan
ubuilder pipeline run --run-id release-1
ubuilder pipeline status --run release-1
```

## Conditional release on tags

```yaml
jobs:
  package:
    steps:
      - uses: builtin/checkout@v1
      - uses: builtin/build@v1
      - uses: builtin/package@v1
  publish:
    needs: [package]
    if: ${{ startsWith(inputs.tag, "v") }}
    steps:
      - uses: builtin/download-artifact@v1
        with:
          pattern: "*"
```

```console
ubuilder pipeline run --input tag=v1.2.3
```

## Dependency cache

```yaml
jobs:
  build:
    steps:
      - uses: builtin/checkout@v1
      - uses: builtin/cache@v1
        id: cargo
        with:
          key: cargo-windows-v1
          path: .cargo
```

`hashFiles` is not part of the expression language. Bump the literal key when
the lockfile changes. Cache paths are workspace-relative and must remain inside
the job workspace.

## Continue on error

```yaml
jobs:
  lint:
    steps:
      - run: cargo fmt --check
        continue-on-error: true
      - run: cargo test
```

The lint step reports `status: failure, conclusion: success`; the job continues.

## Concurrency across terminals

```yaml
jobs:
  publish:
    concurrency:
      group: release-upload
      max: 1
```

Two simultaneous runs in separate shells wait for the SQLite lease instead of
uploading at the same time.

## Useful inspection commands

```console
ubuilder pipeline graph --format mermaid
ubuilder pipeline graph --format dot
ubuilder pipeline graph --format json
ubuilder pipeline inspect --file .ubuilder/workflows/ci.yml --json
ubuilder pipeline history --limit 20
ubuilder pipeline logs --run release-1 --job build --step step-3 --tail 100
ubuilder pipeline cancel release-2
ubuilder pipeline recover --stale-secs 3600
```
