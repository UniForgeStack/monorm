// The platform key of a build: `${process.platform}-${process.arch}`, or `linux-<arch>-musl` when `MONORM_CARGO_TARGET` names a musl target.
// A musl build runs on a glibc host (zig is the cross linker), so `process.platform` cannot name it.
export function platformOf(target = process.env.MONORM_CARGO_TARGET) {
  if (!target?.includes("-musl")) return `${process.platform}-${process.arch}`;
  const arch = { x86_64: "x64", aarch64: "arm64" }[target.split("-")[0]];
  if (!arch) throw new Error(`no musl platform key for the target ${target}`);
  return `linux-${arch}-musl`;
}
