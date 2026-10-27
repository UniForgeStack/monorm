// The package build exposes this module; the fixture tests compile it to CommonJS.
export const manifestHash = "eae14354d3d5506de40f184c1c834c24f8cb3171a24729cfc1cc1e6ff899f61d";

export const OutsideQuerySet = <T extends new (...args: any[]) => any>(Base: T) => class extends Base {
  tagged() { return this.limit(7); }
};

export class OutsideInstance {
  tagLabel(): string { return (this as unknown as { tag: string }).tag; }
}
