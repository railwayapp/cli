# Embedded vt100 terminal emulator

Source: vt100 0.16.2, https://github.com/doy/vt100-rust, MIT (see LICENSE).

Embedded as a module so Railway binaries and published source packages use the
same implementation. Upstream does not retain OSC 8 hyperlink destinations.

Local changes:
- Rewrite internal crate paths to `crate::vt100` and remove upstream lint policy.
- Retain OSC 8 targets on cells, including wide continuations, and clear them on
  overwrite/erase. Cell ownership naturally preserves links through scrolling,
  alternate screens, line insertion/deletion, and resize.
- Bound and validate web hyperlink destinations; ignore unsupported schemes.

Hyperlinks are consumed by Railway's pane click handler, not serialized into
terminal screen dumps. Regression coverage lives in the pane session tests.
