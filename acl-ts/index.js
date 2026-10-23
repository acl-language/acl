// minimal napi-rs loader (no npm toolchain required)
const { existsSync } = require('fs');
const { join } = require('path');
const candidates = [
  join(__dirname, `acl_ts.${process.platform}-${process.arch}.node`),
  join(__dirname, 'acl_ts_linux_x64.node'),   // underscore variant
  join(__dirname, 'acl_ts.node'),
];
const file = candidates.find(existsSync);
if (!file) throw new Error(`acl-ts native module not found in ${__dirname}`);
module.exports = require(file);
