import aclts from '../acl-ts/index.js';
const { AclKey, AclEnvelope, writeFrame, readFrame, newMsgId } = aclts;

const key = new AclKey();
const pub = key.publicBytes();;
const env = new AclEnvelope('local:ts:client', 'did:example:t', 'request',
  JSON.stringify({ t: 'Task', v: { description: 'ts-test', input: { text: 'node' } } }),
  null, null);
env.sign(key);
const raw = env.encode();
const back = AclEnvelope.decode(raw);
if (back.msg_id !== env.msg_id) throw new Error('msg_id mismatch');
if (!back.verify(pub)) throw new Error('verify failed');
const body = JSON.parse(back.bodyJson);
if (body.v.description !== 'ts-test') throw new Error('body mismatch');
// framing
const framed = writeFrame(raw);
if (readFrame(framed).toString('hex') !== raw.toString('hex')) throw new Error('framing mismatch');
// tamper: wrong key must fail
const wrong = new AclKey();
if (back.verify(wrong.publicBytes())) throw new Error('tamper check failed');
console.log('msg_id:', env.msgId, '| msgId via new:', newMsgId().length === 32);
console.log('=== ACL-TS BINDING PASS: sign/verify/decode/framing/tamper OK ===');
