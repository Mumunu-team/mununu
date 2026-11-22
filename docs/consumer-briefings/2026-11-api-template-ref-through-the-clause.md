# Consumer briefing — the API's `template_ref` is realized through the CTXDSL `template` clause (mununu#966)

> **Audience:** maintainers of the repositories that call `POST /api/v1/context/eval` (and the MCP `context_eval` tool) with a `template_ref` instead of a `formula` — mununu-ui, monono, rosf. Not a release note.
>
> Source of truth: [`resolve_template_ref_for_cache`](../../crates/mununu-core/src/api/handlers.rs#L3875) · [`template_ref_sidecar`](../../crates/mununu-core/src/api/handlers.rs#L3897) · [`context_dsl::string_literal`](../../crates/mununu-core/src/context_dsl/mod.rs#L38) — surface: API (CLI+API for the CTXDSL clause itself)

## TL;DR

A request with `template_ref: { template, args }` used to be instantiated inside the API handler (its own catalog build, its own error texts) and the expanded formula written into a synthesized sidecar. Now the sidecar carries the CTXDSL **`template` clause** itself — the same clause a user writes in a `.ctxdsl` file — and realization expands it, the one instantiation path. Three things a consumer can see:

1. **The formula's name is the template id.** The result row for a `template_ref` is named after the template (`reachable`), where it used to be the registry's generated name (`reachable_TARGET_dim`: id plus every parameter in the template's order). Match results by the request you made, not by a reconstructed name; the `formula_name` field carries the new name.
2. **Error texts for a bad reference are realization's.** An unknown template, a missing or ill-typed argument comes back as a `400` whose message is the realizer's (`Failed to parse/realize context: … template `reachable`: unknown template …`) instead of `Template instantiation failed: …`. The condition and the status code are unchanged.
3. **Argument values are written as string literals, escaped.** A value is user input placed into generated CTXDSL text; it is now a quoted literal with `"`, `\`, line breaks and tabs escaped (`context_dsl::string_literal`), so a value cannot close the clause or declare anything. A value that is a bare identifier or a number behaves as before (the registry validates it against the parameter's type, as it did).

4. **An id, an argument key or an automaton name that is not a CTXDSL identifier is refused with a `400`** naming the position (follow-up to this change, found in review: those three sit in identifier positions of the generated text, where escaping is no help, and an id such as `x { } } } context evil {` could close the sidecar's blocks). Letters, digits and `_`, not starting with a digit, not a keyword — the lexer's own rule.

The cache key still covers the template and its arguments (the sidecar text is deterministic: bindings in key order), so repeated identical requests still hit the realization cache.

## What does not change

`formula` requests; the CLI's `--template` / `--template-arg` (the CLI's `resolve_formula_name` and `loader.rs` still instantiate directly with their `--templates-file` packs — out of this change's scope, noted on the PR); `GET /api/v1/templates`; the realized formula itself (byte-for-byte the registry's instantiation for the same reference — pinned by `a_template_ref_realizes_to_the_registrys_own_instantiation`).

Also in this change: the verify orchestrator's property-seed pass used to fall back to the built-in catalog silently when the deployment's template packs did not load; it now logs the reason (`property seeds: the configured template catalog did not load …`). The seeds are telemetry; the run is unaffected.

## Per consumer

### mununu-ui · monono · rosf

- A consumer that reads `formula_name` from a `template_ref` result and expected the generated `id_PARAM_value` form reads the template id now. Nothing else in the response shape moves; `docs/api-schemas/` is unchanged.
- A consumer that matched on the text `Template instantiation failed` for a bad reference matches the realizer's `template `<id>`:` prefix instead.

## Docker rebuild

| image | rebuild | why |
|---|---|---|
| `mununu` (product) | yes | the API handler |
| `mununu-sva` (dev / e2e) | yes | same |
| consumers' own images | no | no schema change |

## Provenance

Review item Design #5 of the 2026-11-09 tier-B review (via mununu#966); the founder decided to take both parts (the shared constructor landed in #1097). Seam tests: `a_template_ref_realizes_to_the_registrys_own_instantiation`, `hostile_argument_values_stay_inside_their_binding` (`}`, `;`, `"`, backslash, line break, a whole fake declaration), `a_string_literal_round_trips_through_the_lexer_whatever_it_holds`.

## Not covered here

The CLI's two direct instantiation sites (above). Whether the realizer should accept an AST `TemplateSpec` from the API instead of text (which would make the escaping moot) was considered and not done: the cache key is computed over sidecar text, and the escaper is exactly the lexer's inverse.
