> **Alpha Software** — Mununu is under active development. APIs, syntax, and behavior may change. We welcome feedback and bug reports via [GitHub Issues](https://github.com/Mumunu-team/mununu/issues).

# Web UI Guide

The Mununu web interface ([mununu-ui](https://github.com/Mumunu-team/mununu-ui)) is a React/TypeScript application that provides an interactive environment for writing CTXDSL specifications, visualizing automata, synthesizing controllers, and verifying mu-calculus properties. It communicates with the Mununu REST API and renders results in real time.

---

## Table of Contents

- [Setup](#setup)
- [Overview](#overview)
- [Editor Tab](#editor-tab)
- [Summary Panel](#summary-panel)
- [Graphs Tab](#graphs-tab)
- [Verification Tab](#verification-tab)
- [Keyboard Shortcuts](#keyboard-shortcuts)
- [Troubleshooting](#troubleshooting)

---

## Setup

### Prerequisites

- Node.js 18 or later
- A running Mununu API server (see [API Reference](./API-Reference))

### Installation

```bash
git clone https://github.com/Mumunu-team/mununu-ui.git
cd mununu-ui
npm install
npm run dev
```

The development server starts on `http://localhost:5173` by default.

### Configuration

Set the API server URL through the `VITE_API_URL` environment variable:

```bash
# Default: http://localhost:3000
VITE_API_URL=http://localhost:3000 npm run dev
```

For production builds:

```bash
VITE_API_URL=https://mununu-api.example.com npm run build
```

The built assets are output to `dist/` and can be served by any static file server.

---

## Overview

The UI is organized into three primary tabs:

| Tab | Purpose |
|-----|---------|
| **Editor** | Write and edit CTXDSL specifications with syntax highlighting. |
| **Graphs** | Visualize automata, compositions, and controllers as interactive graphs. |
| **Verification** | Evaluate mu-calculus formulas and inspect results, counterstrategies. |

A persistent **Summary Panel** on the right side displays context metadata whenever the specification parses successfully.

The general workflow is:

1. Write or load a CTXDSL specification in the Editor tab.
2. Review the Summary Panel to confirm automata, formulas, and controllers parsed correctly.
3. Switch to the Graphs tab to visualize system structure.
4. Switch to the Verification tab to check properties and diagnose failures.

---

## Editor Tab

The editor is built on [Monaco Editor](https://microsoft.github.io/monaco-editor/) (the same engine that powers VS Code) and provides a first-class editing experience for CTXDSL files.

### Syntax Highlighting

The editor recognizes CTXDSL keywords and structures:

- **Keywords**: `context`, `automata`, `automaton`, `states`, `state`, `initial`, `transitions`, `transition`, `on`, `alphabet`, `label`, `mu_formulas`, `formula`, `over`, `body`, `composition`, `controller`, `synthesize`, `minimize`, `enums`, `enum`, `constants`, `const`, `ranges`, `range`, `parameters`, `param`, `in`, `state_groups`, `group`, `wildcard`, `variables`, `var`, `guard`, `effects`, `controllable`, `internal`
- **Operators**: `->`, `mu`, `nu`, `<>`, `[]`, `&&`, `||`, `!`
- **Comments**: `//` line comments

### Auto-completion

Context-aware completions are provided for:

- Top-level block keywords (`automata`, `mu_formulas`, `composition`)
- State and label references within transitions
- Automaton names within formula `over` declarations
- Formula names within `controller synthesize` declarations

### Error Markers

When the specification fails to parse, the editor displays:

- Red squiggly underlines on the offending line(s)
- Error details in the Problems panel below the editor
- The Summary Panel shows the parse error message

Errors are updated as you type with a short debounce delay to avoid flickering during active editing.

### Sidecar Files

The editor supports loading multiple files. Use the file tab bar above the editor to:

- Add sidecar files (properties, additional automata, overlay specifications)
- Switch between the main context and sidecar files
- All files are sent together in API requests

---

## Summary Panel

The Summary Panel appears on the right side of the interface and updates automatically whenever the CTXDSL source is modified and parses successfully. It calls the `/api/v1/context/summarize` endpoint.

### Contents

- **Context Name**: The identifier declared in the `context` block.
- **Automata List**: Each automaton with its state count and transition count. Compositions are included.
- **Formulas Count**: Number of mu-calculus formulas defined.
- **Controller Declarations**: For each declared controller:
  - Source automaton and target formula
  - Realizability status (green check or red cross)
  - State and transition counts of the synthesized controller

The panel provides a quick at-a-glance health check: if all controllers show as realizable and the counts look reasonable, the specification is likely correct before running full verification.

---

## Graphs Tab

The Graphs tab renders automata and controllers as interactive directed graphs using [Cytoscape.js](https://js.cytoscape.org/). It calls the `/api/v1/context/graphs` endpoint.

### Graph Types

- **DSL Graph**: Shows the automaton as declared in the CTXDSL source, with symbolic guards and effects on transitions. This is the "design-level" view.
- **Unrolled Graph**: Shows the fully expanded state space after variable abstraction and guard evaluation. Each node represents a concrete state valuation.

Toggle between graph types using the dropdown above the graph canvas.

### Interaction

- **Pan**: Click and drag on the background.
- **Zoom**: Mouse wheel or pinch gesture.
- **Select**: Click a node or edge to highlight it and see details in the info panel.
- **Fit**: Double-click the background to fit all elements in view.
- **Layout**: Use the layout dropdown to switch between automatic layout algorithms (dagre, breadthfirst, circle, grid).

### Node Styling

| Style | Meaning |
|-------|---------|
| Green diamond border | Initial state (class `start` from the backend) |
| Gray border | Normal state |
| Invisible (zero-size) | Entry helper node for initial state arrows |

### Edge Styling

| Style | Meaning |
|-------|---------|
| Blue solid line | Controllable transition |
| Red dashed line | Uncontrollable transition |
| Thin gray line (no label) | Start arrow pointing to initial state |
| Label text | Action name, guard, effect |

These styles are shared between the Graphs tab and the Counterstrategy graph in the Verification tab (via a common `graphStyles.ts` module).

### Controller Graphs

When the specification declares controllers and the **Include Controllers** toggle is enabled, synthesized controller graphs appear as additional entries in the graph selector. Each controller graph shows:

- Only the states and transitions retained by the controller
- Controllable vs. uncontrollable transitions (by line style)
- Initial states (bold border)

Use the **Minimize Controllers** toggle to apply bisimulation minimization to controller graphs, reducing visual clutter for large controllers.

---

## Verification Tab

The Verification tab is the unified analysis view. It evaluates mu-calculus formulas, reports realizability, and provides on-demand counterstrategy graphs and counterexample traces -- combining what was previously split across separate verification and synthesis tabs. It calls the `/api/v1/context/verify` endpoint for formula evaluation and the `/api/v1/context/synthesize` endpoint for countertraces.

### Running Verification

1. Optionally enter a formula name and/or automaton name to filter (leave empty to evaluate all formulas).
2. Click **Verify**. Results appear in the results table within seconds for typical specifications.

### Results Table

Each row represents one formula-automaton pair:

| Column | Description |
|--------|-------------|
| **Formula** | Formula name. |
| **Automaton** | Target automaton name. |
| **Status** | "Satisfied" (green) or "Not Satisfied" (red). |
| **Satisfying** | `satisfying / total` state counts. |
| **Initial** | `satisfying / total` initial state counts. |
| **Actions** | Counterstrategy and Countertraces buttons (for unsatisfied formulas). |

### Counterstrategy

For formulas that are **not satisfied**, a **Counterstrategy** button appears in the row. Clicking it:

1. Fetches a minimized counterstrategy from the API (formula inversion + evaluation).
2. Expands an inline graph view showing the **environment winning region** -- the set of states from which the environment can force a property violation regardless of the controller's choices.
3. The counterstrategy graph only includes states **reachable from initial states** via the kept transitions (unreachable states are filtered out).
4. Controllable transitions are shown in blue, uncontrollable in red dashed lines, and initial states as green diamonds.

Click the button again to collapse the graph.

### Countertraces

A **Countertraces** button appears next to the Counterstrategy button for unsatisfied formulas. Clicking it runs synthesis internally to compute diagnostic traces:

- **Lasso traces** (shown when available): infinite counterexample paths in `prefix -> (cycle)^ω` format. Each arrow shows the transition label taken between states.
- **Deadlock traces** (shown when no lasso traces exist): finite paths leading to deadlock states.
- **Violating initial states**: listed at the top of the expanded section.

The Countertraces button is hidden if synthesis returns no traces.

### Interpreting Results

- **All Satisfied**: The system meets all specified properties from every initial state. No controller synthesis is needed for these properties.
- **Not Satisfied, Realizable**: The property does not hold on the open system, but a controller can be synthesized to enforce it. Go to the Editor tab and add a `controller` declaration.
- **Not Satisfied, Unrealizable**: The environment can force a violation regardless of any controller strategy. Review the counterstrategy graph and countertraces to understand the adversarial scenario, then consider relaxing the property or restricting the environment model.

---

## Keyboard Shortcuts

| Shortcut | Action |
|----------|--------|
| `Ctrl+S` / `Cmd+S` | Trigger summarize (save and parse) |
| `Ctrl+Enter` / `Cmd+Enter` | Run verification |
| `Ctrl+Shift+G` / `Cmd+Shift+G` | Switch to Graphs tab |
| `Ctrl+Shift+V` / `Cmd+Shift+V` | Switch to Verification tab |
| `Ctrl+Shift+E` / `Cmd+Shift+E` | Switch to Editor tab |

---

## Troubleshooting

### "Failed to fetch" or network errors

The UI cannot reach the Mununu API server. Verify that:

1. The API server is running (`curl http://localhost:3000/api/v1/health`).
2. `VITE_API_URL` points to the correct address and port.
3. No firewall or proxy is blocking the connection.

### Editor shows no syntax highlighting

Clear the browser cache and reload. Monaco language definitions are loaded asynchronously and may fail silently on first load with a cold cache.

### Graphs tab is empty

Ensure the specification parses without errors (check the Summary Panel). The graphs endpoint requires a valid, realized context. If only certain automata are missing, verify they are referenced in a `composition` or `automata` block.

### Verification runs slowly

Large state spaces (10,000+ states after unrolling) increase evaluation time. Consider:

- Reducing variable domains in abstraction blocks.
- Verifying formulas one at a time instead of "Verify All."
- Using the CLI for batch verification of large models.

---

## Further Reading

- [API Reference](./API-Reference) -- Full REST API documentation.
- [References](./References) -- Academic background on mu-calculus, synthesis, and verification.
- [mununu-ui repository](https://github.com/Mumunu-team/mununu-ui) -- Source code, issues, and contribution guidelines.
