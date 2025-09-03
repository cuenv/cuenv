#!/usr/bin/env bash
# License header verification script for cuenv project
# Verifies that all Rust source files have proper license headers

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m' # No Color

# Expected license headers (MIT OR Apache-2.0)
MIT_HEADER="// SPDX-License-Identifier: MIT"
APACHE_HEADER="// SPDX-License-Identifier: Apache-2.0"
DUAL_HEADER="// SPDX-License-Identifier: MIT OR Apache-2.0"

# Alternative headers (older format)
MIT_ALT_HEADER="// Licensed under the MIT License"
APACHE_ALT_HEADER="// Licensed under the Apache License, Version 2.0"

# Counter for issues
ISSUES=0
CHECKED=0

echo -e "${GREEN}🔍 Checking license headers in Rust source files...${NC}"

# Find all Rust source files, excluding target directories and generated files
while IFS= read -r -d '' file; do
    CHECKED=$((CHECKED + 1))
    
    # Read first few lines of the file
    HEAD_LINES=$(head -n 10 "$file")
    
    # Check if any expected license header is present
    if echo "$HEAD_LINES" | grep -q -E "(SPDX-License-Identifier|Licensed under)" &&
       echo "$HEAD_LINES" | grep -q -E "(MIT|Apache-2\.0)"; then
        echo -e "✅ $file"
    else
        echo -e "${RED}❌ Missing or invalid license header: $file${NC}"
        echo -e "${YELLOW}   Expected one of:${NC}"
        echo -e "${YELLOW}     $DUAL_HEADER${NC}"
        echo -e "${YELLOW}     $MIT_HEADER${NC}"
        echo -e "${YELLOW}     $APACHE_HEADER${NC}"
        echo ""
        ISSUES=$((ISSUES + 1))
    fi
    
done < <(find . -name "*.rs" -type f \
    -not -path "./target/*" \
    -not -path "./.git/*" \
    -not -path "./vendor/*" \
    -not -name "build.rs" \
    -print0)

echo ""
echo -e "${GREEN}📊 Summary:${NC}"
echo -e "   Files checked: $CHECKED"
echo -e "   Issues found: $ISSUES"

if [ $ISSUES -eq 0 ]; then
    echo -e "${GREEN}✅ All license headers are present and valid!${NC}"
    exit 0
else
    echo ""
    echo -e "${RED}❌ Found $ISSUES files with missing or invalid license headers.${NC}"
    echo -e "${YELLOW}📋 To fix, add one of these headers to the top of each file:${NC}"
    echo ""
    echo -e "${YELLOW}For dual licensing (recommended):${NC}"
    echo -e "   ${DUAL_HEADER}"
    echo ""
    echo -e "${YELLOW}For MIT only:${NC}"
    echo -e "   ${MIT_HEADER}"
    echo ""
    echo -e "${YELLOW}For Apache-2.0 only:${NC}"
    echo -e "   ${APACHE_HEADER}"
    echo ""
    exit 1
fi
