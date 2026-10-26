// The package build exposes this module; the fixture tests compile it to CommonJS.
export const manifestHash = "7d1acfa50bb44f0ffc2faa307c15400b1b65ce637dc254b80137421605bc7306";

export const OutsideQuerySet = <T extends new (...args: any[]) => any>(Base: T) => class extends Base {
  tagged() { return this.limit(7); }
};

export class OutsideInstance {
  tagLabel(): string { return (this as unknown as { tag: string }).tag; }
}
