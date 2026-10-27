// The package build exposes this module; the fixture tests compile it to CommonJS.
export const manifestHash = "7c2492d6a32cb0483f4d6d644e6a64f11e759bc1b6ecbc1755b1adffd2c2c1bb";

export const OutsideQuerySet = <T extends new (...args: any[]) => any>(Base: T) => class extends Base {
  tagged() { return this.limit(7); }
};

export class OutsideInstance {
  tagLabel(): string { return (this as unknown as { tag: string }).tag; }
}
