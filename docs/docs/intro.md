---
sidebar_position: 0
title: Introduction
---

# Introduction

systemg is a **general-purpose program composer** that transforms arbitrary programs into coherent systems with explicit lifecycles, dependencies, and health monitoring. Rather than managing individual daemons or containers, systemg focuses on composition—how programs relate, start, roll, and recover together.

Running on top of existing OS primitives like systemd and cgroups, systemg inherits their stability while adding higher-level intent. It turns a collection of processes into a system you can reason about, evolve, and deploy cleanly, with built-in support for lifecycle webhooks and cron-like scheduling.
