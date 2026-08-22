#!/bin/sh
# Three child shapes an operator actually hits:
sleep 3600 &                                   # 1 plain child
sh -c 'sleep 3600' &                           # 2 grandchild under a shell
setsid sh -c 'sleep 3600' >/dev/null 2>&1 &    # 3 double-forked, own session
echo ready > /ready/w
exec sleep 3600
