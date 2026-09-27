# Contributing

How to work in this repository — the stack, the style and the vocabulary —
is in [CLAUDE.md](CLAUDE.md). This file covers how a change lands.

## Before you push

```
just ci
```

It runs exactly what CI's Check job runs, in the same order. `just check` is
the same thing under its older name.

## PR titles are commit messages

PRs are squash-merged, and the squash commit takes the PR's title. That
commit is what release-please reads to choose the next version and write the
changelog, so the title must be a
[conventional commit](https://www.conventionalcommits.org/):

```
<type>[(scope)][!]: <subject>
```

- **type** — one of `feat fix docs chore refactor perf test build ci revert`.
- **scope** — optional, e.g. `refactor(report): …`.
- **subject** — starts lowercase, no trailing period.
- **`!`** — marks a breaking change.

The PR Title check enforces this, and re-runs when you edit the title.

| Type | Version bump while 0.x | Changelog |
|---|---|---|
| `feat` | minor | Features |
| `fix` | patch | Bug Fixes |
| `feat!`, `fix!`, … | minor | noted as breaking |
| `perf`, `refactor`, `docs`, `build`, `revert`, `chore` | patch | their own section |
| `ci`, `test` | none | hidden |

## Releases

Nobody tags by hand. release-please keeps a release PR open against `main`;
merging it tags `vX.Y.Z`, bumps `Cargo.toml`, and publishes
`ghcr.io/hmbill694/assembly-line:X.Y.Z`.
