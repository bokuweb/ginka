# Upgrading GPUI

Ginka never pins `gpui` itself. The rev comes transitively from
[`gpui-component`](https://github.com/longbridge/gpui-component), which pins
`gpui` from `zed-industries/zed` git. Pinning it independently guarantees a
conflict the next time the toolkit moves — see `docs/roadmap.md` §4.6.

## Procedure

1. Upgrades are their own PR. Never bundle one with a feature change: when the
   window stops rendering you want a one-commit blast radius.
2. `cargo update -p gpui-component` (and `-p gpui-component-assets`).
3. `cargo build` and read the breakage. GPUI's element and context APIs move
   between revs; expect signature churn in `Render`, `Context` and event
   handlers rather than deep behavioural change.
4. Run the app and check, at minimum: window translucency, the sidebar and
   right-panel resize handles, terminal rendering, and theme switching. These
   are the four things that have broken across GPUI revs before.

   Translucency is the fragile one. The toolkit paints from two places — the
   `colors` palette and the `tokens` derived from it — and `Root` uses the
   derived side. `ginka_ui::theme::apply` regenerates `tokens` after writing
   `colors`; if a future rev adds a third source, an opaque window is the
   symptom.
5. Note the old and new revs in the PR description.

## Constraints

- **Do not add a second GPUI component library.** `bezel` links
  `bezel-gpui`, a republished fork under a different package name, so its
  `App`/`Window`/`Element` types are unrelated to ours at the type level. Code
  ported from bezel must be adapted to our `gpui`, not linked.
- If a fix is needed upstream, prefer a PR to `gpui-component` over a fork. If a
  fork becomes unavoidable, record it in the roadmap's decision log with the
  condition under which we drop it.
