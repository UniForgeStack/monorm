/** Classes without a protobuf API: the user registers a codec for them. */
export class Plain {
  constructor(readonly text = "") {}
}

export class Served {
  constructor(readonly text = "") {}
}
