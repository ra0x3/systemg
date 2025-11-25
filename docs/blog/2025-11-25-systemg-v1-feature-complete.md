---
slug: systemg-v1-feature-complete
title: Systemg 1.0.0 is feature complete! 🥳
authors: [rashad]
tags: [announcement, release, systemg, v1]
---

After a few months of development and testing, I'm excited to announce that **Systemg 1.0.0 is feature complete!**

> ⚠️ This does not mean that v1.0.0 is about to be released, it's merely feature complete.

What started as a simple process manager has evolved into a robust, production-ready tool that handles everything from basic service management to complex deployment workflows. I've been running sysg in production on [Arbitration](https://arbi.gg), and it's been serving my needs perfectly — managing services reliably without the complexity of systemd.

<!-- truncate -->

## What's New in v1

Here's a rundown of the major features that made it into v1:

- **Webhooks** — Fire HTTP requests on service lifecycle events (start, stop, restart). Perfect for notifications, deployment pipelines, and service orchestration.

- **Cron Support** — Schedule tasks with cron-style expressions. Run periodic jobs alongside your always-on services without needing a separate cron daemon.

- **Zero Downtime Deployments** — Gracefully restart services without dropping connections. Essential for production environments where uptime matters.

- **Root-level Environment Configuration** — Define environment variables once and share them across all services. Makes configuration management much cleaner.

- **Skip Functionality** — Temporarily disable services without removing them from your config. Useful for maintenance and testing scenarios.

- **Pre-start Hooks** — Run commands before service startup. Great for health checks, migrations, or environment setup.

- **Configurable Log Levels** — Control verbosity with `--log-level` flag. Debug when you need it, quiet when you don't.

- **Dependency Management** — Services can depend on other services. Sysg handles startup order and ensures dependencies are running.

- **Cross-platform Support** — Full support for Linux (glibc, musl/Alpine), Debian, and macOS (Intel + Apple Silicon).

- **Session-based Process Management** — Proper process group handling ensures clean shutdowns and signal propagation.

## Battle-tested in Production

I've been using sysg to manage services on [Arbitration](https://arbi.gg) for months now. It handles everything from web servers to background workers, proving itself reliable and predictable. No surprises, no weird edge cases — just solid process management.

## What's Next?

With v1 feature complete, the focus shifts to:
- Stability and bug fixes
- Performance optimizations
- Community feedback and feature requests
- Documentation improvements

Try it out and let me know what you think!

Check out the [full documentation](/docs/intro) to get started.
