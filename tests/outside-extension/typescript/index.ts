// The package build exposes this module; the fixture tests compile it to CommonJS.
export const manifestHash = "d6bac6daf540a1c02ba194c380a7749882c7d635659cb5e56c5bae6f3f9504e1";

export const OutsideQuerySet = <T extends new (...args: any[]) => any>(Base: T) => class extends Base {
  tagged() { return this.limit(7); }
};

export class OutsideInstance {
  tagLabel(): string { return (this as unknown as { tag: string }).tag; }
}
