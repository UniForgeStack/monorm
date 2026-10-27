// The package build exposes this module; the fixture tests compile it to CommonJS.
export const manifestHash = "9076582855268d3ab76646c7794e1dfd6ed4c2588d010c8c04b4be477a99ca8b";

export const OutsideQuerySet = <T extends new (...args: any[]) => any>(Base: T) => class extends Base {
  tagged() { return this.limit(7); }
};

export class OutsideInstance {
  tagLabel(): string { return (this as unknown as { tag: string }).tag; }
}
