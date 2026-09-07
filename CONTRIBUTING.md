# Contributing to Mununu

Thank you for your interest in contributing to Mununu!

## Prerequisites

- **Rust 1.91+** &mdash; Install via [rustup](https://rustup.rs/)
- **Node.js 20+** &mdash; Only needed if working on [mununu-ui](https://github.com/Mumunu-team/mununu-ui)

## Getting Started

```bash
# Clone the repository
git clone https://github.com/Mumunu-team/mununu.git
cd mununu

# Install pre-commit hooks (required)
./scripts/setup-hooks.sh

# Build and run tests
cargo build
cargo test
```

## Development Workflow

1. Create a branch from `main`
2. Make your changes
3. Ensure all checks pass locally:
   ```bash
   cargo fmt --all -- --check
   cargo clippy --all-targets -- -D warnings
   cargo test --lib --bins --tests --examples
   ```
4. Commit and open a pull request against `main`

The pre-commit hook runs formatting, linting, and tests automatically.

## Code Style

- Run `cargo fmt` before committing
- All clippy warnings are treated as errors (`-D warnings`)
- Prefer existing utilities over hand-rolling common tasks
- Remove unused code and dependencies promptly
- Write test names that describe behavior, not implementation

## Project Structure

See the [Architecture section](README.md#architecture) in the README for an overview of the source tree.

## Adding Examples

New `.ctxdsl` examples go in `examples/`. Each example should:

- Include a header comment explaining the system being modeled
- Define explicit `controllable` sections for each automaton
- Include at least one mu-calculus formula for verification
- Work with `cargo run -- context summarize examples/your_example.ctxdsl`

## Reporting Issues

Open an issue on GitHub with:

- A clear description of the problem or feature request
- Steps to reproduce (for bugs)
- The `.ctxdsl` file that triggers the issue (if applicable)

## License

By contributing, you agree that your contributions will be licensed under the same [Mununu Non-Commercial License](LICENSE) as the project.
