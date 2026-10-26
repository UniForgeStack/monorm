#!/bin/sh
ORM_PROFILE=postgres python -m orm check
npx orm generate
