const url = process.env.ORM_DATABASE_URL ?? process.env.ORM_TEST_DATABASE_URL;
const profile = process.env.ORM_PROFILE;
export { url, profile };
