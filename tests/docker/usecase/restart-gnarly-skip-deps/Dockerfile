FROM sysg-usecase-base

COPY tests/docker/usecase/restart-gnarly-skip-deps/stack.v1.yaml /usecase/stack.v1.yaml
COPY tests/docker/usecase/restart-gnarly-skip-deps/stack.v2.yaml /usecase/stack.v2.yaml
COPY tests/docker/usecase/restart-gnarly-skip-deps/stack.v3.yaml /usecase/stack.v3.yaml
COPY tests/docker/usecase/restart-gnarly-skip-deps/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
