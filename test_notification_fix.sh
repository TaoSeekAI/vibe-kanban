#!/bin/bash

echo "Testing notification service with DBus protection..."
echo ""

# Test 1: Normal operation with DBus available
echo "Test 1: Normal operation (DBus should be checked)"
RUST_LOG=debug,services::services::notification=trace cargo run --bin vibe-backend 2>&1 | grep -A5 -B5 "notification" &
PID=$!
sleep 5
kill $PID 2>/dev/null
echo ""

# Test 2: Operation with DBus disabled
echo "Test 2: With DISABLE_DBUS_NOTIFICATIONS environment variable"
DISABLE_DBUS_NOTIFICATIONS=1 RUST_LOG=debug,services::services::notification=trace cargo run --bin vibe-backend 2>&1 | grep -A5 -B5 "notification" &
PID=$!
sleep 5
kill $PID 2>/dev/null
echo ""

echo "Test complete. Check logs above for:"
echo "1. DBus availability check with timeout"
echo "2. Proper handling when DISABLE_DBUS_NOTIFICATIONS is set"
echo "3. No deadlocks or hanging processes"