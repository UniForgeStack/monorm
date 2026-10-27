// The package build exposes this module; the fixture tests compile it to CommonJS.
export const manifestHash = "8f9e297e69cdd1c697e3410da819b4dc0a596e04ba9439163e062acd4f427f5a";

export const OutsideQuerySet = <T extends new (...args: any[]) => any>(Base: T) => class extends Base {
  tagged() { return this.limit(7); }
};

export class OutsideInstance {
  tagLabel(): string { return (this as unknown as { tag: string }).tag; }
}
