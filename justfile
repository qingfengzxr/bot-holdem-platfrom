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

test-anvil:
  mise check:test-anvil

run-app:
  mise dev:app

run-anvil:
  mise dev:anvil

run-local-multi-bot-demo:
  mise dev:local-multi-bot-demo

note:
  @echo "Repository commands should be organized in mise; use just as a convenience wrapper."
