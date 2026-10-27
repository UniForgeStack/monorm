// The package build exposes this module; the fixture tests compile it to CommonJS.
export const manifestHash = "4795be01f8107922ca014e8766850287849e0d7f7e323cb16cc24c141067a866";

export const OutsideQuerySet = <T extends new (...args: any[]) => any>(Base: T) => class extends Base {
  tagged() { return this.limit(7); }
};

export class OutsideInstance {
  tagLabel(): string { return (this as unknown as { tag: string }).tag; }
}
