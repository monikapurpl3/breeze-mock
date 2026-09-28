#!/bin/sh
# The schema half of tests/blueprints.rs, for machines with xmllint but no
# Rust: every example must validate against schema/blueprint.xsd, and every
# bad fixture must be rejected or accepted exactly as its "xsd:" marker says.
set -u
cd "$(dirname "$0")/.."
fail=0
for f in examples/*.xml; do
  if ! xmllint --noout --nonet --schema schema/blueprint.xsd "$f" >/dev/null 2>&1; then
    echo "FAIL  $f: the schema rejects an example"; fail=1
  fi
done
n=0
for f in tests/fixtures/bad/*.xml; do
  n=$((n + 1))
  want=$(sed -n 's/.* | xsd: \(rejects\|accepts\) -->$/\1/p' "$f" | head -1)
  if xmllint --noout --nonet --schema schema/blueprint.xsd "$f" >/dev/null 2>&1; then got=accepts; else got=rejects; fi
  if [ "$got" != "$want" ]; then
    echo "FAIL  $f: marked xsd: $want, but the schema $got it"; fail=1
  fi
done
[ "$fail" = 0 ] && echo "schema agrees: $(ls examples/*.xml | wc -l) examples, $n bad fixtures"
exit "$fail"
