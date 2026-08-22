#!/bin/sh
sleep "$1"
echo r > /ready/w
exec sleep 3600
