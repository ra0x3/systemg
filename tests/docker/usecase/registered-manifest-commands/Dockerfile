FROM sysg-usecase-base

COPY tests/docker/usecase/registered-manifest-commands/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/registered-manifest-commands/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
