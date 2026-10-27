// The package build exposes this module; the fixture tests compile it to CommonJS.
export const manifestHash = "49078d804127b38d85523fa45f79c7259143261ba617603a6d273b2864ce5295";

export const OutsideQuerySet = <T extends new (...args: any[]) => any>(Base: T) => class extends Base {
  tagged() { return this.limit(7); }
};

export class OutsideInstance {
  tagLabel(): string { return (this as unknown as { tag: string }).tag; }
}
