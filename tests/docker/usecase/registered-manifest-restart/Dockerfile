FROM sysg-usecase-base

COPY tests/docker/usecase/registered-manifest-restart/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/registered-manifest-restart/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
