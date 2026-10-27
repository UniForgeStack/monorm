// The package build exposes this module; the fixture tests compile it to CommonJS.
export const manifestHash = "8b906a854eeb936ef57a6296ddafe942d7acb212c57259fcecaf6d0b84eb45c2";

export const OutsideQuerySet = <T extends new (...args: any[]) => any>(Base: T) => class extends Base {
  tagged() { return this.limit(7); }
};

export class OutsideInstance {
  tagLabel(): string { return (this as unknown as { tag: string }).tag; }
}
