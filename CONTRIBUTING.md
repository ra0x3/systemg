# Contributing to systemg

Thank you for your interest in contributing to systemg! This document provides guidelines and instructions for contributing to the project.

## Code of Conduct

By participating in this project, you agree to abide by our Code of Conduct. Please read it before contributing.

## How to Contribute

### Reporting Bugs

- Before creating a bug report, check the issue tracker to see if the problem has already been reported.
- When creating a bug report, include detailed steps to reproduce the issue, the expected behavior, and the actual behavior.
- Include information about your environment, such as OS, Rust version, and any relevant configuration.

### Suggesting Enhancements

- Enhancement suggestions are tracked as GitHub issues.
- Provide a clear description of the enhancement and the motivation for it.
- If possible, outline a potential implementation approach.

### Pull Requests

1. Fork the repository.
2. Create a new branch for your feature or bug fix.
3. Make your changes and ensure they follow the project's coding style.
4. Add tests for your changes when applicable.
5. Run the existing tests to ensure they still pass.
6. Commit your changes following the commit message conventions.
7. Submit a pull request with a clear description of the changes.

## Development Setup

### Prerequisites

- Rust (latest stable version)
- Cargo
- Git

### Setting Up the Development Environment

```sh
# Clone your fork of the repository
git clone https://github.com/YOUR_USERNAME/systemg.git
cd systemg

# Add the upstream repository
git remote add upstream https://github.com/ra0x3/systemg.git

# Install development dependencies
cargo build
```

## Coding Guidelines

### Rust Style

- Follow the Rust style guidelines and idioms.
- Use `cargo fmt` to format your code.
- Use `cargo clippy` to catch common mistakes and improve your code.

### Documentation

- Document public API functions, types, and modules.
- Use Rust's documentation comment style (`///` for documentation, `//` for regular comments).
- Keep documentation up-to-date with code changes.

### Testing

- Write tests for new features and bug fixes.
- Ensure existing tests pass with your changes.
- Follow the existing testing patterns in the project.

## Commit Message Guidelines

- Use the present tense ("Add feature" not "Added feature").
- Use the imperative mood ("Move cursor to..." not "