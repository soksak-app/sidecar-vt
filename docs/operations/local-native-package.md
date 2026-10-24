# Local native package development

The sidecar manifest keeps its deployment dependency pinned to the declared
core Git revision. A local core checkout can provide the native package during
development without changing that pin or committing pnpm's generated link
override.

Run dependency-sensitive repository checks before linking the local package.
`pnpm exec soksak-records` can run pnpm resolution and restore the manifest's
deployment pin after generated local link metadata has been removed. Then run:

```sh
pnpm install --frozen-lockfile --no-optional
pnpm link <absolute-core-native-process-identity-package-directory>
node -e "const p=require.resolve('@soksak/native-process-identity-darwin/package.json');console.log(p,require(p).version)"
make build
make release OUT=<new-output-directory> SOK=<core>/target/debug/sok
```

Confirm that `require.resolve` points into the intended core checkout and that
the package contains `include/soksak_process_identity.h`,
`lib/libsoksak-process-identity.a`, and
`lib/pkgconfig/soksak-process-identity.pc`. Run `make build` and `make release`
after the local link is in place. Keep the generated pnpm workspace and lock
override out of version control; keep `package.json` and the committed lockfile
on the declared deployment pin. Preserve the generated release before running
a pnpm command that may resolve dependencies again.

The local link is a development resolution only. It does not verify a fresh
installation of the deployment pin or publish the sidecar.
