FROM gcr.io/oss-fuzz-base/base-builder-rust

COPY . $SRC/tokenix
WORKDIR $SRC/tokenix
COPY .clusterfuzzlite/build.sh $SRC/build.sh
