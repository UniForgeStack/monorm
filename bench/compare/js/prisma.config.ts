import { defineConfig } from "prisma/config";

export default defineConfig({
  schema: "prisma/schema.prisma",
  datasource: { url: process.env.BENCH_DATABASE_URL ?? "postgres://postgres:postgres@localhost:55432/bench_nfx1" },
});
