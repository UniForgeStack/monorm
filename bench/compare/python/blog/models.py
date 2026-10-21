"""The shared schema (../../schema.sql) as Django models. The tables exist already: managed = False."""

from django.db import models


class User(models.Model):
    email = models.CharField(max_length=254, unique=True)
    name = models.CharField(max_length=100)
    created_at = models.DateTimeField()

    class Meta:
        managed = False
        db_table = "users"


class Category(models.Model):
    name = models.CharField(max_length=50, unique=True)

    class Meta:
        managed = False
        db_table = "categories"


class Tag(models.Model):
    name = models.CharField(max_length=50, unique=True)

    class Meta:
        managed = False
        db_table = "tags"


class Post(models.Model):
    author = models.ForeignKey(User, on_delete=models.CASCADE, related_name="posts")
    category = models.ForeignKey(Category, null=True, on_delete=models.SET_NULL, related_name="posts")
    title = models.CharField(max_length=200)
    body = models.TextField()
    views = models.IntegerField(default=0)
    published = models.BooleanField(default=False)
    created_at = models.DateTimeField()
    tags = models.ManyToManyField(Tag, through="PostTag", related_name="posts")

    class Meta:
        managed = False
        db_table = "posts"


class Comment(models.Model):
    post = models.ForeignKey(Post, on_delete=models.CASCADE, related_name="comments")
    author = models.ForeignKey(User, null=True, on_delete=models.SET_NULL, related_name="comments")
    body = models.TextField()
    created_at = models.DateTimeField()

    class Meta:
        managed = False
        db_table = "comments"


class PostTag(models.Model):
    post = models.ForeignKey(Post, on_delete=models.CASCADE)
    tag = models.ForeignKey(Tag, on_delete=models.CASCADE)

    class Meta:
        managed = False
        db_table = "post_tags"
        unique_together = [("post", "tag")]
