import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
const fixture = JSON.parse(readFileSync(new URL('./fixtures/peri-1.0.json', import.meta.url)));

// Independent framing reader: never imports the Rust encoder or generated output.
function object(bytes) {
  assert.ok(bytes.length >= 25);
  const size = bytes.readUInt32BE(21);
  assert.ok(size > 0 && size <= 1048576);
  assert.ok(bytes[20] <= 1);
  assert.equal(bytes.length, 25 + size);
  return {
    sequence: Number(bytes.readBigUInt64BE(0)), timestamp_us: Number(bytes.readBigUInt64BE(8)),
    duration_us: bytes.readUInt32BE(16), independent: bytes[20] === 1,
    payload: [...bytes.subarray(25)],
  };
}
test('PERI 1.0 fixed framing vectors and malformed objects', () => {
  const { hex, ...expected } = fixture.object;
  const bytes = Buffer.from(hex, 'hex');
  assert.deepEqual(object(bytes), expected);
  assert.throws(() => object(bytes.subarray(0, -1)));
  assert.throws(() => object(Buffer.concat([bytes, Buffer.from([0])])));
  for (const [offset, value] of [[20, 2], [21, 255]]) {
    const invalid = Buffer.from(bytes); invalid[offset] = value;
    assert.throws(() => object(invalid));
  }
  const control = Buffer.from(fixture.control.hex, 'hex');
  assert.equal(control.readUInt32BE(0), control.length - 4);
  assert.deepEqual(JSON.parse(control.subarray(4).toString('utf8')), fixture.control.json);
  assert.equal(Buffer.from(fixture.preface, 'hex').length, 8);
});
