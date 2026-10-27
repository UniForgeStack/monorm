// The package build exposes this module; the fixture tests compile it to CommonJS.
export const manifestHash = "cd1e3298e6d0efbd6210d6007c72658999faed2332aae60ff9ef79da75640a2a";

export const OutsideQuerySet = <T extends new (...args: any[]) => any>(Base: T) => class extends Base {
  tagged() { return this.limit(7); }
};

export class OutsideInstance {
  tagLabel(): string { return (this as unknown as { tag: string }).tag; }
}
