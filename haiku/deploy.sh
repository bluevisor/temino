#!/bin/sh
# Builds Temino natively on the Haiku VM and (re)starts it there.
#   haiku/deploy.sh            build + run
#   haiku/deploy.sh --no-run   build only
# HOST defaults to the UTM debug VM's ssh alias.
set -e
HOST=${HOST:-haiku-dbg}
cd "$(dirname "$0")/.."

# The VM clock resets to 1970 on boot; cargo and tar both mind.
ssh "$HOST" "date -u -s '$(date -u '+%Y-%m-%d %H:%M:%S')'" >/dev/null

COPYFILE_DISABLE=1 tar --no-xattrs --exclude ./target --exclude ./dist --exclude ./.git -czf - . \
	| ssh "$HOST" 'rm -rf ~/src/temino/src && mkdir -p ~/src/temino && tar xzf - -C ~/src/temino 2>/dev/null'
ssh "$HOST" 'cd ~/src/temino && cargo build --release 2>&1 | grep -Ev "^\s+(Compiling|Downloaded|Downloading)" | tail -40'

if [ "$1" != "--no-run" ]; then
	# Haiku has no pkill; the team id is the 4th-from-last field.
	ssh "$HOST" 'for t in $(ps | grep "/temino" | grep -v grep | awk "{print \$(NF-3)}"); do kill $t; done; true'
	ssh "$HOST" 'cd ~ && (~/src/temino/target/release/temino > ~/config/settings/Temino-stdout.log 2>&1 &)'
	echo "temino started on $HOST (log: ~/config/settings/Temino-stdout.log)"
fi
