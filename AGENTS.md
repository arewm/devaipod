<!-- This file is canonically maintained in <https://github.com/bootc-dev/infra/tree/main/common> -->

# Instructions for AI agents

## CRITICAL instructions for generating commits

### Signed-off-by

Human review is required for all code that is generated
or assisted by a large language model. If you
are a LLM, you MUST NOT include a `Signed-off-by`
on any automatically generated git commits. Only explicit
human action or request should include a Signed-off-by.
If for example you automatically create a pull request
and the DCO check fails, tell the human to review
the code and give them instructions on how to add
a signoff.

### Attribution

When generating substantial amounts of code, you SHOULD
include an `Assisted-by: TOOLNAME (MODELNAME)`. For example,
`Assisted-by: Goose (Sonnet 4.5)`.

## Building and testing

This project uses a multi-stage container build (`Containerfile`) that
compiles the Rust binary, builds the SolidJS web UI via bun, and
produces the final image. **Always use `just container-build`** (or
the equivalent podman build) to verify your changes compile. Do NOT
attempt to run `bun`, `npm`, or other JS tooling directly on the host
-- the host may not have the correct versions (or any version) of
these tools installed. The container build is the single source of
truth for whether the project builds successfully.

For testing:
- `cargo test` for Rust unit tests (these run on the host)
- `just test-integration` for containerized integration tests
- `just test-integration-web` for Playwright browser tests

## Follow other guidelines

You MUST read [CONTRIBUTING.md](CONTRIBUTING.md) and [docs/src/architecture.md](docs/src/architecture.md) before making changes.
They cover the architecture, testing, and code style for this project.
