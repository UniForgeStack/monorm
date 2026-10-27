// The package build exposes this module; the fixture tests compile it to CommonJS.
export const manifestHash = "622d0638cfb6b3f37587844ea260a8d567eea62fe87c45ecb18f352ae6906575";

export const OutsideQuerySet = <T extends new (...args: any[]) => any>(Base: T) => class extends Base {
  tagged() { return this.limit(7); }
};

export class OutsideInstance {
  tagLabel(): string { return (this as unknown as { tag: string }).tag; }
}
