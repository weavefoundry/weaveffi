# 07 — Eight Registries, One IDL (Single Post)

The distribution angle. "Generates bindings" sells the engineering. "Ship
to eight package managers" sells the *outcome*.

---

## Hook

> Where can you `install` your library after one `weaveffi generate`?
>
>   npm · PyPI · SwiftPM · Maven · NuGet · pub.dev · RubyGems · Go modules
>
> Generated packages are standalone. Consumers never install WeaveFFI.

---

## Image — the install wall

A single image showing the rendered "Install" line per ecosystem. Use
each registry's official wordmark/color so it's instantly recognizable.

```
npm     ➜  npm install   @you/contacts
PyPI    ➜  pip install   you-contacts
Swift   ➜  Package.swift: .package(url: "github.com/you/contacts-swift")
Maven   ➜  implementation("com.you:contacts:1.0.0")
NuGet   ➜  dotnet add package You.Contacts
pub.dev ➜  flutter pub add contacts
gem     ➜  gem install   contacts
Go      ➜  go get        github.com/you/contacts
```

Render in [carbon.now.sh](https://carbon.now.sh), or build the badge wall
in Figma using each registry's color.

---

## Body (single post or reply)

> WeaveFFI's design principle: the generated packages are **standalone**.
> Your TypeScript user does not need to install `weaveffi`. Your Ruby
> user does not need to install `weaveffi`. They install your gem and
> it works.
>
> Helper code (error types, memory utils) is inlined into each package.
> Your consumers never know WeaveFFI exists. Which is the point.

---

## Why this works

- **Concrete outcome.** Lists of package managers are scroll-stopping
  because every developer recognises at least three of them.
- **Defuses the "but my consumers" objection.** The "standalone" line
  pre-empts the most common pushback.
- **Reusable.** Pin it to your profile during launch week.

---

## Alt text

"A formatted list of eight package-manager install commands — one for
npm, PyPI, SwiftPM, Maven, NuGet, pub.dev, RubyGems, and Go modules —
each tied to a sample library called 'contacts'."
