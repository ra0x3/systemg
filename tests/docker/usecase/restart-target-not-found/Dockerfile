FROM sysg-usecase-base

COPY tests/docker/usecase/restart-target-not-found/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/restart-target-not-found/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
