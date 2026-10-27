// The package build exposes this module; the fixture tests compile it to CommonJS.
export const manifestHash = "b5c4d2f71cda38fb002bf627b8802fdd1997cfe698a0885d036351f8a94f43d4";

export const OutsideQuerySet = <T extends new (...args: any[]) => any>(Base: T) => class extends Base {
  tagged() { return this.limit(7); }
};

export class OutsideInstance {
  tagLabel(): string { return (this as unknown as { tag: string }).tag; }
}
