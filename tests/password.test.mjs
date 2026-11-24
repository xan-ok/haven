import assert from "node:assert/strict";
import test from "node:test";

import {
  PASSWORD_MAX_CHARS,
  PASSWORD_MIN_CHARS,
  countPasswordChars,
  evaluateStrength,
  validateCreation,
} from "../src/lib/password.js";

test("按 Unicode 码点计数", () => {
  assert.equal(countPasswordChars(""), 0);
  assert.equal(countPasswordChars("😀😀"), 2);
  assert.equal(countPasswordChars("a b"), 3);
  assert.equal(countPasswordChars("密码 短语"), 5);
  assert.equal(countPasswordChars("e\u0301"), 2);
});

test("12 个字符是创建下限", () => {
  assert.equal(PASSWORD_MIN_CHARS, 12);
  assert.equal(
    validateCreation({
      password: "12345678901",
      confirm: "12345678901",
      acknowledged: true,
    }).valid,
    false
  );
  assert.equal(
    validateCreation({
      password: "123456789012",
      confirm: "123456789012",
      acknowledged: true,
    }).valid,
    true
  );
});

test("空格计入长度且不会被 trim", () => {
  const password = "    four    ";
  assert.equal(countPasswordChars(password), 12);
  assert.equal(
    validateCreation({ password, confirm: password, acknowledged: true }).valid,
    true
  );
  assert.equal(
    validateCreation({
      password: password.trim(),
      confirm: password.trim(),
      acknowledged: true,
    }).valid,
    false
  );
});

test("Unicode 密码按码点满足下限", () => {
  const password = "😀".repeat(12);
  assert.equal(countPasswordChars(password), 12);
  assert.equal(
    validateCreation({ password, confirm: password, acknowledged: true }).valid,
    true
  );
});

test("超过 1024 个字符被拒绝", () => {
  assert.equal(PASSWORD_MAX_CHARS, 1024);
  const tooLong = "a".repeat(1025);
  assert.equal(
    validateCreation({ password: tooLong, confirm: tooLong, acknowledged: true }).valid,
    false
  );
  const max = "a".repeat(1024);
  assert.equal(
    validateCreation({ password: max, confirm: max, acknowledged: true }).valid,
    true
  );
});

test("两次输入不一致或未确认时不可创建", () => {
  assert.equal(
    validateCreation({
      password: "123456789012",
      confirm: "123456789013",
      acknowledged: true,
    }).reason,
    "mismatch"
  );
  assert.equal(
    validateCreation({
      password: "123456789012",
      confirm: "123456789012",
      acknowledged: false,
    }).reason,
    "unacknowledged"
  );
});

test("强度分为空、偏弱、一般、较强", () => {
  assert.equal(evaluateStrength("").level, "empty");
  assert.equal(evaluateStrength("abc").level, "weak");
  assert.equal(evaluateStrength("abcdefghijkl").level, "weak");
  assert.equal(evaluateStrength("正确 Horse 电池 钉书针").level, "medium");
  assert.equal(evaluateStrength("correct horse battery staple").level, "strong");
  assert.equal(evaluateStrength("Correct-Horse-Battery-Staple-2026!").level, "strong");
});
