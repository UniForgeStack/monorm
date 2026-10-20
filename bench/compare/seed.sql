-- Deterministic seed: 1,000 users, 20 categories, 10,000 posts, 50,000 comments, 50 tags, 30,000 post-tag links.
-- Every value derives from the row number, so every contender reads the same rows.
INSERT INTO users (id, email, name, created_at)
SELECT n, 'user' || n || '@example.com', 'User ' || n, timestamptz '2026-01-01 00:00:00+00' + n * interval '1 minute'
FROM generate_series(1, 1000) AS n;

INSERT INTO categories (id, name)
SELECT n, 'category-' || n FROM generate_series(1, 20) AS n;

INSERT INTO tags (id, name)
SELECT n, 'tag-' || n FROM generate_series(1, 50) AS n;

-- 30% of the posts (n % 10 < 3) have no category: the nullable foreign key.
INSERT INTO posts (id, author_id, category_id, title, body, views, published, created_at)
SELECT n, (n - 1) % 1000 + 1, CASE WHEN n % 10 < 3 THEN NULL ELSE n % 20 + 1 END,
       'Post ' || n, 'Body of post ' || n || ' ' || md5(n::text), (n * 37) % 1000, n % 3 <> 0,
       timestamptz '2026-01-01 00:00:00+00' + n * interval '1 minute'
FROM generate_series(1, 10000) AS n;

-- Every 7th comment has no author.
INSERT INTO comments (id, post_id, author_id, body, created_at)
SELECT n, (n - 1) % 10000 + 1, CASE WHEN n % 7 = 0 THEN NULL ELSE n % 1000 + 1 END,
       'Comment ' || n || ' ' || md5(n::text), timestamptz '2026-01-01 00:00:00+00' + n * interval '1 minute'
FROM generate_series(1, 50000) AS n;

-- Three distinct tags per post: (n + 7k) % 50 + 1 for k = 0, 1, 2.
INSERT INTO post_tags (post_id, tag_id)
SELECT n, (n + 7 * k) % 50 + 1 FROM generate_series(1, 10000) AS n, generate_series(0, 2) AS k;

SELECT setval(pg_get_serial_sequence('users', 'id'), 1000, true);
SELECT setval(pg_get_serial_sequence('categories', 'id'), 20, true);
SELECT setval(pg_get_serial_sequence('tags', 'id'), 50, true);
SELECT setval(pg_get_serial_sequence('posts', 'id'), 10000, true);
SELECT setval(pg_get_serial_sequence('comments', 'id'), 50000, true);
SELECT setval(pg_get_serial_sequence('post_tags', 'id'), (SELECT max(id) FROM post_tags), true);
ANALYZE;
