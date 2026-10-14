import assert from "node:assert/strict";
import test from "node:test";
import { path } from "../dist/path.js";

test("POSIX path helpers", () => {
  assert.equal(path.join("/a/b", "../c", "./d/"), "/a/c/d");
  assert.equal(path.join("/a", "/b"), "/b");
  assert.equal(path.join("/", ".."), "/");
  assert.equal(path.join("a", "..", ".."), "..");
  assert.equal(path.dirname("/a/b/"), "/a");
  assert.equal(path.dirname("/a"), "/");
  assert.equal(path.dirname("/"), "/");
  assert.equal(path.basename("/a/b.txt"), "b.txt");
  assert.equal(path.basename("/"), "/");
});
