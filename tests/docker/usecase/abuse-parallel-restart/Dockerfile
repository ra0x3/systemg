FROM sysg-usecase-base

COPY tests/docker/usecase/abuse-parallel-restart/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/abuse-parallel-restart/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
