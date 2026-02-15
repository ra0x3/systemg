---
sidebar_position: 2
title: Installation
---

# Installation

## Install

### Linux / macOS

![Installation](https://i.imgur.com/6d2aq0U.gif)

```bash
$ curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev/ | sh
```

## Verify

```bash
$ sysg --version
```

## Specific version

Install a specific version:

```bash
$ curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev/ | sh -s -- --version 0.15.6
```

## Switch versions

Switch to an already installed version, or download it:

```bash
$ curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev/ | sh -s -- -v 0.15.5
```

## Next steps

Create your first [configuration](quickstart#create-a-configuration) and start running services.
