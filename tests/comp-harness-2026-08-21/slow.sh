#!/bin/sh
sleep 5
echo r > /ready/w
exec sleep 3600
