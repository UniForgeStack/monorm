// The package build exposes this module; the fixture tests compile it to CommonJS.
export const manifestHash = "f30c328d3fd652ba0ee61a1f4c95bf78aba69db678bb75791038a955736c5839";

export const OutsideQuerySet = <T extends new (...args: any[]) => any>(Base: T) => class extends Base {
  tagged() { return this.limit(7); }
};

export class OutsideInstance {
  tagLabel(): string { return (this as unknown as { tag: string }).tag; }
}
