import { createRequire } from 'node:module'

const require = createRequire(import.meta.url)
const contacts = require('../../generated/node/index.js')

const h1 = contacts.createContact('Alice', 'Smith', 'alice@example.com', 0)
console.log('Created contact #' + h1)

const h2 = contacts.createContact('Bob', 'Jones', null, 1)
console.log('Created contact #' + h2)

const count = contacts.countContacts()
console.log('\nTotal: ' + count + ' contacts\n')

const list = contacts.listContacts()
for (const c of list) {
  const email = c.email ? ' <' + c.email + '>' : ''
  const types = ['Personal', 'Work', 'Other']
  const label = types[c.contactType] || 'Unknown'
  console.log(`  [${c.id}] ${c.firstName} ${c.lastName}${email} (${label})`)
}
