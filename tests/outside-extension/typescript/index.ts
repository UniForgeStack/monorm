// The package build exposes this module; the fixture tests compile it to CommonJS.
export const manifestHash = "1dc9051159c140647e07c82040688ce8100bc70d819e2b92a85f6a38e1e3d0c9";

export const OutsideQuerySet = <T extends new (...args: any[]) => any>(Base: T) => class extends Base {
  tagged() { return this.limit(7); }
};

export class OutsideInstance {
  tagLabel(): string { return (this as unknown as { tag: string }).tag; }
}
