#!/bin/bash

set -e

ruff format .
ruff check --fix .
ruff check .