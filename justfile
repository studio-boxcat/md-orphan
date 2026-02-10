default: build

# https://github.com/swiftlang/swift/blob/main/docs/OptimizationTips.rst
# SwiftPM release already enables -O + WMO; -cross-module-optimization added in Package.swift
build:
    swift build -c release
    cp .build/release/md-orphan dist/md-orphan

install: build
    ln -sf {{justfile_directory()}}/dist/md-orphan /usr/local/bin/md-orphan

test:
    swift test

run *ARGS:
    swift run -c release md-orphan {{ARGS}}

clean:
    swift package clean
