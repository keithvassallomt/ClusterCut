---
name: Check existing dependencies first
description: Before suggesting new crates/libraries, check what's already in the dependency tree that could solve the problem
type: feedback
---

Before reaching for external crates to solve a problem, check what's already available in the existing dependency tree. In this project, GTK was already a transitive dependency via Tauri and could handle clipboard on both X11 and Wayland — but I wasted time investigating wl-clipboard-rs, smithay-clipboard, and arboard first.

**Why:** Unnecessary external dependencies add complexity, build time, and potential incompatibilities. The existing toolkit often already solves the problem.

**How to apply:** When a feature touches an area the app's framework already handles (e.g., clipboard, windowing, notifications), check the framework's own APIs first before looking at standalone crates.
