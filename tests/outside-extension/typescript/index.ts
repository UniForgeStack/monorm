// The package build exposes this module; the fixture tests compile it to CommonJS.
export const manifestHash = "9894b1df7f88bdd60f2ecc0c2924cb04def4099f24f376015ead8fad1a46b790";

export const OutsideQuerySet = <T extends new (...args: any[]) => any>(Base: T) => class extends Base {
  tagged() { return this.limit(7); }
};

export class OutsideInstance {
  tagLabel(): string { return (this as unknown as { tag: string }).tag; }
}
