set positional-arguments

default:
  @just --list

check *args:
  mise check {{args}}

fmt:
  mise check:fmt

lint:
  mise check:clippy

test *args:
  mise check:test {{args}}

run-app:
  mise dev:app

note:
  @echo "Repository commands should be organized in mise; use just as a convenience wrapper."
