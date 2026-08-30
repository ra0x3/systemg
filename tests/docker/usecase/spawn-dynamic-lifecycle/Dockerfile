FROM sysg-usecase-base

COPY tests/docker/usecase/spawn-dynamic-lifecycle/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/spawn-dynamic-lifecycle/orchestrate.sh /usecase/orchestrate.sh
COPY tests/docker/usecase/spawn-dynamic-lifecycle/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh /usecase/orchestrate.sh

ENTRYPOINT ["/usecase/run.sh"]
