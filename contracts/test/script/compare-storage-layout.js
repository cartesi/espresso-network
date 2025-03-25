const { execSync } = require("child_process");

// Strip AST ID from type string (e.g., t_contract(LightClient)44013 → t_contract(LightClient))
function normalizeType(type) {
    // Remove `_storage`, `_memory`, etc. suffixes with IDs
    // Remove any digits after a closing parenthesis or struct name
    return type
      .replace(/\)\d+(_\w+)?/g, ")")                  // e.g., `)44149_storage` → `)`
      .replace(/(_\w+)?\)\d+/g, ")")                  // just in case
      .replace(/\(.*?\)\d+(_\w+)?/g, (match) => match.replace(/\)\d+(_\w+)?/, ")"));
  }

// Extracts the layout using forge inspect and parses the JSON output
function extractLayout(contractName) {
  const output = execSync(`forge inspect ${contractName} storageLayout --json`).toString();
  const layout = JSON.parse(output);
  return layout.storage.map(({ label, slot, offset, type }) => ({
    label,
    slot,
    offset,
    type: normalizeType(type),
  }));
}

// Compare two storage layout arrays
function compareLayouts(layoutA, layoutB) {
  if (layoutA.length > layoutB.length) {
    console.log("false");
    return false;
  }

  for (let i = 0; i < layoutA.length; i++) {
    const a = layoutA[i];
    const b = layoutB[i];

    if (
      a.label !== b.label ||
      a.slot !== b.slot ||
      a.offset !== b.offset ||
      a.type !== b.type
    ) {
      console.error(`Mismatch at index ${i}:\n  A: ${JSON.stringify(a)}\n  B: ${JSON.stringify(b)}`);
      console.log("false");
      return false;
    }
  }

  console.log("true");
  return true;
}

const [contractA, contractB] = process.argv.slice(2);

if (!contractA || !contractB) {
  console.error("Usage: node compare-storage-layout.js oldContractName newContractName");
  process.exit(1);
}

try {
  const layoutA = extractLayout(contractA);
  const layoutB = extractLayout(contractB);
  const success = compareLayouts(layoutA, layoutB);

  process.exit(success ? 0 : 1);
} catch (err) {
  console.error("Error comparing layouts:", err.message);
  process.exit(1);
}
