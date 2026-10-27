// The package build exposes this module; the fixture tests compile it to CommonJS.
export const manifestHash = "09ad796a6a17bdfbf3850971617f5c3fb2bd892812be965d8feca0ea1033c334";

export const OutsideQuerySet = <T extends new (...args: any[]) => any>(Base: T) => class extends Base {
  tagged() { return this.limit(7); }
};

export class OutsideInstance {
  tagLabel(): string { return (this as unknown as { tag: string }).tag; }
}
