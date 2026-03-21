import { createRequire } from 'node:module'

// Require the built N-API addon via generated loader (CommonJS)
const require = createRequire(import.meta.url)
const api = require('../../generated/node/index.js')

console.log('add(3,4) =', api.add(3, 4))
console.log('mul(5,6) =', api.mul(5, 6))
console.log('div(10,2) =', api.div(10, 2))
console.log('echo("hello") =', api.echo('hello'))
try { api.div(1, 0) } catch (e) { console.log('div(1,0) error =', String(e)) }
