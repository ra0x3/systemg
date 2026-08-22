#!/bin/sh
sleep 0.2
echo r > /ready/$1
exec sleep 3600
